use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tracing::{debug, info, warn};
use v4l::capability::Flags as CapFlags;
use v4l::format::FourCC;
use v4l::frameinterval::FrameIntervalEnum;
use v4l::framesize::FrameSizeEnum;
use v4l::video::capture::Parameters;
use v4l::video::Capture as _;
use v4l::{Device, Format};

mod worker;

const FOURCC_YUYV: FourCC = FourCC { repr: *b"YUYV" };
const FOURCC_MJPG: FourCC = FourCC { repr: *b"MJPG" };

pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub bus_info: String,
    pub usb_id: Option<(u16, u16)>,
}

/// capture-capable video nodes only, sorted by path
pub fn list_devices() -> Vec<DeviceInfo> {
    let mut paths: Vec<PathBuf> = match fs::read_dir("/dev") {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| video_index(p).is_some())
            .collect(),
        Err(e) => {
            warn!("cannot read /dev: {e}");
            return Vec::new();
        }
    };
    paths.sort_by_key(|p| video_index(p));

    paths
        .into_iter()
        .filter_map(|path| match describe_device(&path) {
            Ok(info) => info,
            Err(e) => {
                debug!("skip {}: {e:#}", path.display());
                None
            }
        })
        .collect()
}

/// the numeric suffix of /dev/videoN, used for ordering
fn video_index(path: &Path) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix("video")?
        .parse()
        .ok()
}

fn describe_device(path: &Path) -> Result<Option<DeviceInfo>> {
    let device = Device::with_path(path).context("open")?;
    let caps = device.query_caps().context("query caps")?;
    if !caps.capabilities.contains(CapFlags::VIDEO_CAPTURE) {
        return Ok(None);
    }
    // caps.capabilities is device_caps, so metadata nodes are already gone
    // here; this only drops a driver that sets VIDEO_CAPTURE and offers no
    // pixel format at all
    if device.enum_formats().map(|f| f.is_empty()).unwrap_or(true) {
        return Ok(None);
    }
    Ok(Some(DeviceInfo {
        path: path.to_path_buf(),
        name: caps.card,
        bus_info: caps.bus,
        usb_id: read_usb_id(path),
    }))
}

fn read_usb_id(path: &Path) -> Option<(u16, u16)> {
    let node = path.file_name()?.to_str()?;
    let usb = PathBuf::from("/sys/class/video4linux")
        .join(node)
        .join("device")
        .join("..");
    let vendor = fs::read_to_string(usb.join("idVendor")).ok()?;
    let product = fs::read_to_string(usb.join("idProduct")).ok()?;
    parse_usb_id(&vendor, &product)
}

fn parse_usb_id(vendor: &str, product: &str) -> Option<(u16, u16)> {
    let v = u16::from_str_radix(vendor.trim(), 16).ok()?;
    let p = u16::from_str_radix(product.trim(), 16).ok()?;
    Some((v, p))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceFormat {
    Yuyv,
    Mjpeg,
}

impl SourceFormat {
    fn fourcc(self) -> FourCC {
        match self {
            SourceFormat::Yuyv => FOURCC_YUYV,
            SourceFormat::Mjpeg => FOURCC_MJPG,
        }
    }

    fn from_fourcc(fourcc: FourCC) -> Option<SourceFormat> {
        if fourcc == FOURCC_YUYV {
            Some(SourceFormat::Yuyv)
        } else if fourcc == FOURCC_MJPG {
            Some(SourceFormat::Mjpeg)
        } else {
            None
        }
    }
}

pub struct FrameSize {
    pub width: u32,
    pub height: u32,
    /// integer frame rates the driver offers at this size
    pub fps: Vec<u32>,
}

pub struct FormatInfo {
    pub format: SourceFormat,
    pub sizes: Vec<FrameSize>,
}

/// only yuyv and mjpeg entries are reported
pub fn list_formats(path: &Path) -> Result<Vec<FormatInfo>> {
    let device = Device::with_path(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for desc in device.enum_formats().context("enum formats")? {
        let Some(format) = SourceFormat::from_fourcc(desc.fourcc) else {
            continue;
        };
        let mut sizes = Vec::new();
        for size in device
            .enum_framesizes(desc.fourcc)
            .context("enum frame sizes")?
        {
            // stepwise ranges would expand to thousands of entries, so only
            // the largest size of such a range is listed
            let (width, height) = match size.size {
                FrameSizeEnum::Discrete(d) => (d.width, d.height),
                FrameSizeEnum::Stepwise(s) => (s.max_width, s.max_height),
            };
            let mut fps = Vec::new();
            for interval in device
                .enum_frameintervals(desc.fourcc, width, height)
                .context("enum frame intervals")?
            {
                match interval.interval {
                    FrameIntervalEnum::Discrete(f) => {
                        fps.extend(fps_from_interval(f.numerator, f.denominator))
                    }
                    FrameIntervalEnum::Stepwise(s) => {
                        fps.extend(fps_from_interval(s.min.numerator, s.min.denominator));
                        fps.extend(fps_from_interval(s.max.numerator, s.max.denominator));
                    }
                }
            }
            fps.sort_unstable_by(|a, b| b.cmp(a));
            fps.dedup();
            sizes.push(FrameSize { width, height, fps });
        }
        out.push(FormatInfo { format, sizes });
    }
    Ok(out)
}

/// frames per second from a v4l2 interval fraction (seconds per frame)
fn fps_from_interval(numerator: u32, denominator: u32) -> Option<u32> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let fps = (denominator as f64 / numerator as f64).round() as u32;
    (fps > 0).then_some(fps)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelLayout {
    Yuyv,
    /// decoded mjpeg
    Rgba8,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub data: Vec<u8>,
    pub seq: u64,
    pub received: Instant,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub path: PathBuf,
    pub format: SourceFormat,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

type Wake = Box<dyn Fn() + Send + Sync>;

/// state shared between the capture thread and its handle
struct Shared {
    latest: Mutex<Option<Arc<Frame>>>,
    seq: AtomicU64,
    stamps: Mutex<VecDeque<Instant>>,
    stop: AtomicBool,
    wake: Mutex<Option<Wake>>,
    /// why the worker returned, set before it does
    error: Mutex<Option<String>>,
}

const FPS_WINDOW: Duration = Duration::from_secs(1);

impl Shared {
    /// hands back the frame it replaced, so the worker can drop or
    /// recycle it outside the lock
    fn publish(&self, mut frame: Frame) -> Option<Arc<Frame>> {
        let now = frame.received;
        frame.seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let previous = self.latest.lock().replace(Arc::new(frame));
        {
            let mut stamps = self.stamps.lock();
            stamps.push_back(now);
            prune_stamps(&mut stamps, now);
        }
        if let Some(wake) = self.wake.lock().as_ref() {
            wake();
        }
        previous
    }

    fn fail(&self, message: String) {
        *self.error.lock() = Some(message);
    }
}

fn prune_stamps(stamps: &mut VecDeque<Instant>, now: Instant) {
    while let Some(first) = stamps.front() {
        if now.duration_since(*first) > FPS_WINDOW {
            stamps.pop_front();
        } else {
            break;
        }
    }
}

/// owns a capture thread that stops on drop
pub struct Capture {
    config: CaptureConfig,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(cfg: CaptureConfig) -> Result<Capture> {
        let device =
            Device::with_path(&cfg.path).with_context(|| format!("open {}", cfg.path.display()))?;

        let wanted = Format::new(cfg.width, cfg.height, cfg.format.fourcc());
        let got = device.set_format(&wanted).context("set format")?;
        if got.fourcc != wanted.fourcc {
            anyhow::bail!(
                "driver refused {} and offered {} instead",
                wanted.fourcc,
                got.fourcc
            );
        }
        if (got.width, got.height) != (cfg.width, cfg.height) {
            warn!(
                "driver replaced {}x{} with {}x{}",
                cfg.width, cfg.height, got.width, got.height
            );
        }

        // S_PARM must follow S_FMT: uvcvideo resets the interval on a
        // format change
        let params = device
            .set_params(&Parameters::with_fps(cfg.fps))
            .context("set frame rate")?;
        if let Some(fps) = fps_from_interval(params.interval.numerator, params.interval.denominator)
        {
            if fps != cfg.fps {
                warn!("driver replaced {} fps with {fps} fps", cfg.fps);
            }
        }
        info!(
            "capture {} {:?} {}x{} @ {}/{}",
            cfg.path.display(),
            cfg.format,
            got.width,
            got.height,
            params.interval.denominator,
            params.interval.numerator
        );

        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            stamps: Mutex::new(VecDeque::new()),
            stop: AtomicBool::new(false),
            wake: Mutex::new(None),
            error: Mutex::new(None),
        });

        let worker = worker::Worker {
            device,
            width: got.width,
            height: got.height,
            format: cfg.format,
            shared: Arc::clone(&shared),
        };
        let thread = std::thread::Builder::new()
            .name("capture".into())
            .spawn(move || worker.run())
            .context("spawn capture thread")?;

        Ok(Capture {
            config: cfg,
            shared,
            thread: Some(thread),
        })
    }

    /// newest frame, never blocks; the same arc comes back until a newer
    /// frame arrives
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.shared.latest.lock().clone()
    }

    /// rolling one second estimate
    pub fn measured_fps(&self) -> f32 {
        let mut stamps = self.shared.stamps.lock();
        prune_stamps(&mut stamps, Instant::now());
        stamps.len() as f32
    }

    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }

    /// called from the capture thread after every published frame
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.shared.wake.lock() = Some(Box::new(wake));
    }

    /// why the capture thread exited, once it has
    pub fn finished(&self) -> Option<String> {
        let done = self.thread.as_ref().is_none_or(JoinHandle::is_finished);
        done.then(|| {
            self.shared
                .error
                .lock()
                .clone()
                .unwrap_or_else(|| "capture thread exited".into())
        })
    }
}

const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

impl Drop for Capture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let Some(thread) = self.thread.take() else {
            return;
        };
        // JoinHandle has no timed join, so poll is_finished and detach the
        // thread when the driver never hands back a buffer
        let deadline = Instant::now() + JOIN_TIMEOUT;
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                warn!("capture thread did not stop in time, leaving it behind");
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.join().is_err() {
            warn!("capture thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_id_parses_sysfs_hex() {
        assert_eq!(parse_usb_id("046d\n", "08e5\n"), Some((0x046d, 0x08e5)));
        assert_eq!(parse_usb_id("046d", "08e5"), Some((0x046d, 0x08e5)));
        assert_eq!(parse_usb_id("", "08e5"), None);
        assert_eq!(parse_usb_id("zz", "08e5"), None);
        assert_eq!(parse_usb_id("046d", "10000"), None);
    }

    #[test]
    fn fps_rounds_interval() {
        assert_eq!(fps_from_interval(1, 30), Some(30));
        assert_eq!(fps_from_interval(1001, 30000), Some(30));
        assert_eq!(fps_from_interval(1, 24), Some(24));
        assert_eq!(fps_from_interval(2, 15), Some(8));
        assert_eq!(fps_from_interval(0, 30), None);
        assert_eq!(fps_from_interval(1, 0), None);
        assert_eq!(fps_from_interval(10, 1), None);
    }

    #[test]
    fn video_index_reads_suffix() {
        assert_eq!(video_index(Path::new("/dev/video12")), Some(12));
        assert_eq!(video_index(Path::new("/dev/video")), None);
        assert_eq!(video_index(Path::new("/dev/vcs1")), None);
    }

    #[test]
    fn prune_keeps_one_second() {
        let now = Instant::now();
        let mut stamps: VecDeque<Instant> = [3000, 1500, 999, 500, 0]
            .into_iter()
            .map(|ms| now - Duration::from_millis(ms))
            .collect();
        prune_stamps(&mut stamps, now);
        assert_eq!(stamps.len(), 3);
    }

    #[test]
    fn list_devices_does_not_panic() {
        for d in list_devices() {
            assert!(d.path.starts_with("/dev"));
        }
    }
}
